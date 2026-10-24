"""Application entry point: wires config, discovery loop, link, coordinator, tray."""

from __future__ import annotations

import logging
import threading

from . import tscli
from .config import load
from .coordinator import Coordinator
from .tray import TrayApp

log = logging.getLogger(__name__)


def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(name)s %(levelname)s %(message)s",
    )
    cfg = load()
    coord = Coordinator(cfg.device_id, cfg.role, cfg.port, cfg.ping_interval)
    tray: list[TrayApp] = []
    refresh_now = threading.Event()  # set by the tray's "Refresh now" item

    def start_tray() -> None:
        app = TrayApp(
            coord,
            on_quit=lambda: tray[0].stop() if tray else None,
            on_refresh=refresh_now.set,
        )
        tray.append(app)
        app.run()

    def discovery_loop() -> None:
        """Periodically detect the peer via the tailscale CLI.

        The peer is whatever single other device shows up in
        `tailscale status` — nothing about it is persisted. Both roles need
        its IP (as the exit-node target), but only the client dials.
        """
        warned_missing = False
        while True:
            peer = None
            try:
                peer = tscli.detect_peer()
            except tscli.TailscaleError as e:
                log.warning("[discovery] `tailscale status` failed: %s", e)
            if peer is None:
                log.info("[discovery] no single other device found in the tailnet")
                if not warned_missing and tray:
                    tray[0].notify("No other device found in the tailnet.")
                    warned_missing = True
            else:
                log.info("[discovery] peer detected: %s (%s), %s",
                         peer.hostname, peer.ip, "online" if peer.online else "offline")
                warned_missing = False
                coord.set_peer_ip(peer.ip)
                if peer.online and cfg.role == "client":
                    log.info("[discovery] role=client, peer online -> dialing %s", peer.ip)
                    coord.link.dial(peer.ip)

            # Sleep until the next tick, or return early on a manual refresh.
            manual = refresh_now.wait(cfg.status_interval)
            refresh_now.clear()
            if manual:
                log.info("[discovery] manual refresh: peer re-detection done, updating tray")
                if peer is not None:
                    tray[0].notify(
                        f"Peer {peer.hostname} ({peer.ip}) is "
                        f"{'online' if peer.online else 'offline'}."
                    )
                else:
                    tray[0].notify("No other device found in the tailnet.")
                tray[0].refresh()

    threading.Thread(target=discovery_loop, daemon=True, name="discovery").start()
    coord.link.start()
    log.info("tspair started (device_id=%s, role=%s)", cfg.device_id[:8], cfg.role)
    start_tray()  # blocks until Quit


if __name__ == "__main__":
    main()
