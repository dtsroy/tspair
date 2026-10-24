"""Configuration loading / first-run template creation."""

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
    peer_hostname: str   # Tailscale hostname of the friend's device (from `tailscale status`)
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


def _ask_peer_hostname() -> str:
    """Autofill the peer from `tailscale status`: auto-pick when exactly one
    other device exists, otherwise prompt. Falls back to the placeholder when
    tailscale is unavailable or stdin is not a terminal."""
    import sys as _sys
    from . import tscli

    try:
        peers = tscli.list_peers()
    except tscli.TailscaleError as e:
        print(f"Could not query `tailscale status` ({e}); leaving peer unset.")
        return ""
    if not peers:
        print("No other devices found in the tailnet; leaving peer unset.")
        return ""
    if len(peers) == 1:
        p = peers[0]
        print(f"Found a single other device in the tailnet, using it as peer: {p.hostname} ({p.ip})")
        return p.hostname
    if not _sys.stdin.isatty():
        print("Multiple devices found but no terminal to pick from; leaving peer unset.")
        return ""
    print("Which device is your peer?")
    for i, p in enumerate(peers, 1):
        state = "online" if p.online else "offline"
        print(f"  {i}. {p.hostname} ({p.ip}, {state})")
    while True:
        try:
            raw = input(f"Pick 1-{len(peers)} (empty to leave unset): ").strip()
        except EOFError:
            return ""
        if not raw:
            return ""
        if raw.isdigit() and 1 <= int(raw) <= len(peers):
            return peers[int(raw) - 1].hostname
        print("Invalid choice, try again.")


def load() -> Config:
    """Load config; on first run, write a template (peer autofilled from
    `tailscale status` when possible) and abort with instructions."""
    path = Config.default_path()
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        peer_hostname = _ask_peer_hostname() or "REPLACE-WITH-PEER-TAILSCALE-HOSTNAME"
        template = {
            "peer_hostname": peer_hostname,
            "role": "client",  # the friend's device must use "server"
            "port": DEFAULT_PORT,
            "device_id": str(uuid.uuid4()),
        }
        path.write_text(json.dumps(template, indent=2), encoding="utf-8")
        print(f"A config was written to:\n  {path}")
        if peer_hostname.startswith("REPLACE"):
            print("Set 'peer_hostname' to your friend's device hostname\n"
                  "(as shown by `tailscale status`) and start the app again.")
        else:
            print(f"Peer autofilled as '{peer_hostname}'. On that device, set\n"
                  "\"role\" to \"server\" and start the app again.")
        sys.exit(1)

    raw = json.loads(path.read_text(encoding="utf-8"))
    role = str(raw.get("role", "client")).lower()
    if role not in ("server", "client"):
        print(f"Invalid role {role!r} in {path}: must be \"server\" or \"client\".")
        sys.exit(1)
    cfg = Config(
        peer_hostname=str(raw["peer_hostname"]),
        role=role,
        port=int(raw.get("port", DEFAULT_PORT)),
        device_id=str(raw.get("device_id") or uuid.uuid4()),
        ping_interval=float(raw.get("ping_interval", 15.0)),
        status_interval=float(raw.get("status_interval", 30.0)),
    )
    if "REPLACE" in cfg.peer_hostname:
        print(f"'peer_hostname' is not configured yet. Edit {path} and set it to\n"
              "your friend's device hostname (as shown by `tailscale status`).")
        sys.exit(1)
    return cfg
