"""Wire protocol: newline-delimited JSON over TCP.

Message types (each is a dict with a "type" field):

  HELLO   {id, state}      first message on a new connection
  PING    {}               heartbeat
  PONG    {}
  REQ_USE {}               "I want to route via you" (ask permission)
  GRANT   {}               provider approved; requester may set exit node
  DENY    {reason}         provider refused (loop risk / simultaneous request)
  STATE   {state}          broadcast of sender's current ExitState
"""

from __future__ import annotations

import json
from typing import Optional

RECV_BUF = 65536


def encode(msg: dict) -> bytes:
    return (json.dumps(msg, separators=(",", ":")) + "\n").encode("utf-8")


class FrameReader:
    """Accumulates bytes from a socket and yields complete JSON messages."""

    def __init__(self) -> None:
        self._buf = b""

    def feed(self, data: bytes) -> list[dict]:
        self._buf += data
        out = []
        while b"\n" in self._buf:
            line, self._buf = self._buf.split(b"\n", 1)
            if not line.strip():
                continue
            try:
                out.append(json.loads(line.decode("utf-8")))
            except json.JSONDecodeError:
                continue  # ignore malformed lines rather than killing the link
        return out

    def decode_one(self, data: bytes) -> Optional[dict]:
        msgs = self.feed(data)
        return msgs[0] if msgs else None
