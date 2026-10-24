"""Configuration loading / first-run template creation.

The peer is never stored here — it is detected fresh from `tailscale status`
on every discovery cycle (see tscli.detect_peer).
"""

from __future__ import annotations

import json
import os
import sys
import uuid
from dataclasses import dataclass
from pathlib import Path

CONFIG_NAME = "tspair.json"
DEFAULT_PORT = 52520


@dataclass
class Config:
    role: str = "client"  # "server" (listens) or "client" (dials) — exactly one of each per pair
    port: int = DEFAULT_PORT
    device_id: str = ""  # random UUID, used for simultaneous-request tie-breaking
    ping_interval: float = 15.0
    status_interval: float = 30.0

    @staticmethod
    def default_path() -> Path:
        """Config lives in the project root (repo folder), on every platform."""
        env = os.environ.get("TSPAIR_CONFIG")
        if env:
            return Path(env)
        return Path(__file__).resolve().parent.parent / CONFIG_NAME


def load() -> Config:
    """Load config; on first run, write a template and abort with instructions."""
    path = Config.default_path()
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        template = {
            "role": "client",  # the peer's device must use "server"
            "port": DEFAULT_PORT,
            "device_id": str(uuid.uuid4()),
        }
        path.write_text(json.dumps(template, indent=2), encoding="utf-8")
        print(f"A config was written to:\n  {path}")
        print("On the peer's device, set \"role\" to \"server\" and start the app again.")
        sys.exit(1)

    raw = json.loads(path.read_text(encoding="utf-8"))
    role = str(raw.get("role", "client")).lower()
    if role not in ("server", "client"):
        print(f"Invalid role {role!r} in {path}: must be \"server\" or \"client\".")
        sys.exit(1)
    return Config(
        role=role,
        port=int(raw.get("port", DEFAULT_PORT)),
        device_id=str(raw.get("device_id") or uuid.uuid4()),
        ping_interval=float(raw.get("ping_interval", 15.0)),
        status_interval=float(raw.get("status_interval", 30.0)),
    )
