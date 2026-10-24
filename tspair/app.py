"""Application entry point: wires config, discovery loop, link, coordinator, tray."""

from __future__ import annotations

import logging
import threading
import time

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

    def start_tray() -> None:
        app = TrayApp(coord, on_quit=lambda: tray[0].stop() if tray else None)
        tray.append(app)
        app.run()

    def discovery_loop() -> None:
        """Periodically find the peer via the tailscale CLI.

        Both roles need the peer's IP (as the exit-node target), but only the
        client dials — the server just listens.
        """
        warned_missing = False
        while True:
            try:
                info = tscli.find_peer(cfg.peer_hostname)
            except tscli.TailscaleError as e:
                log.warning("tailscale status failed: %s", e)
                info = None
            if info is None:
                if not warned_missing and tray:
                    tray[0].notify(f"Device '{cfg.peer_hostname}' not found in the tailnet.")
                    warned_missing = True
            else:
                warned_missing = False
                coord.set_peer_ip(info.ip)
                if info.online and cfg.role == "client":
                    coord.link.dial(info.ip)
            time.sleep(cfg.status_interval)

    threading.Thread(target=discovery_loop, daemon=True, name="discovery").start()
    coord.link.start()
    log.info("tspair started (device_id=%s, peer=%s)", cfg.device_id[:8], cfg.peer_hostname)
    start_tray()  # blocks until Quit


if __name__ == "__main__":
    main()
