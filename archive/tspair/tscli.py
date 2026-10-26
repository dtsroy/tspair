"""Thin wrapper around the `tailscale` CLI — the single source of truth
for peer discovery and exit-node state."""

from __future__ import annotations

import json
import subprocess
from dataclasses import dataclass
from typing import Optional

TS = "tailscale"  # resolved from PATH; tailscale.exe on Windows


class TailscaleError(RuntimeError):
    pass


def _run(args: list[str], timeout: float = 15.0) -> str:
    try:
        proc = subprocess.run(
            [TS, *args], capture_output=True, text=True, timeout=timeout,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
    except FileNotFoundError as e:
        raise TailscaleError("tailscale CLI not found on PATH") from e
    except subprocess.TimeoutExpired as e:
        raise TailscaleError(f"tailscale {' '.join(args)} timed out") from e
    if proc.returncode != 0:
        raise TailscaleError(
            f"tailscale {' '.join(args)} failed: {(proc.stderr or proc.stdout).strip()}"
        )
    return proc.stdout


@dataclass
class PeerInfo:
    hostname: str
    online: bool
    ip: str  # peer's Tailscale IP (100.x.y.z)
    advertised_exit_node: bool = False  # peer advertises itself as an exit node


@dataclass
class PeerEntry:
    hostname: str
    dns_name: str
    ip: str
    online: bool
    advertised_exit_node: bool = False


def list_peers() -> list[PeerEntry]:
    """All *other* devices in the tailnet (excludes this device)."""
    data = json.loads(_run(["status", "--json"]))
    peers = []
    for p in (data.get("Peer") or {}).values():
        ips = p.get("TailscaleIPs") or []
        peers.append(PeerEntry(
            hostname=str(p.get("HostName", "")),
            dns_name=str(p.get("DNSName", "")).rstrip("."),
            ip=ips[0] if ips else "",
            online=bool(p.get("Online")),
            advertised_exit_node=bool(p.get("AdvertisedExitNode")),
        ))
    return peers


def detect_peer() -> Optional[PeerInfo]:
    """The one other device in the tailnet, from `tailscale status --json`.

    The pair setup assumes this device plus exactly one peer; returns None
    when that doesn't hold (no other device, or more than one).
    """
    peers = list_peers()
    if len(peers) != 1:
        return None
    p = peers[0]
    return PeerInfo(hostname=p.hostname, online=p.online, ip=p.ip,
                    advertised_exit_node=p.advertised_exit_node)


def set_exit_node(peer_ip: str) -> None:
    """Start routing this device's traffic via the peer (USING_OTHER)."""
    _run(["set", f"--exit-node={peer_ip}"])


def clear_exit_node() -> None:
    """Stop using the peer as exit node, back to normal routing (NONE)."""
    _run(["set", "--exit-node="])


def advertise_exit_node(on: bool) -> None:
    """Advertise (or stop advertising) this device as an exit node (BEING_USED)."""
    _run(["set", f"--advertise-exit-node={'true' if on else 'false'}"])


def is_using_exit_node() -> bool:
    """True if this device currently routes via any exit node."""
    try:
        data = json.loads(_run(["status", "--json"]))
    except (TailscaleError, json.JSONDecodeError):
        return False
    # Newer clients expose ExitNodeStatus when an exit node is in use.
    return bool(data.get("ExitNodeStatus"))
