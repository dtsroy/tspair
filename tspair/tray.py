"""System-tray presence: one icon, a small dynamic menu, notifications."""

from __future__ import annotations

import logging
import threading
from typing import Callable

import pystray
from PIL import Image, ImageDraw

from .coordinator import Coordinator
from .exit_state import ExitState, LABELS

log = logging.getLogger(__name__)

APP_TITLE = "tspair"


def _make_image(state: ExitState) -> Image.Image:
    """16x16 placeholder icon; color hints at the exit-node state."""
    colors = {
        ExitState.NONE: (120, 120, 120),        # gray
        ExitState.USING_OTHER: (240, 180, 40),  # amber
        ExitState.BEING_USED: (70, 160, 240),   # blue
    }
    img = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.ellipse([8, 8, 56, 56], fill=colors[state])
    d.ellipse([24, 24, 40, 40], fill=(255, 255, 255, 220))
    return img


class TrayApp:
    def __init__(self, coord: Coordinator, on_quit: Callable[[], None]) -> None:
        self.coord = coord
        self._on_quit = on_quit
        coord.on_change = self.refresh
        self.icon = pystray.Icon(
            APP_TITLE, _make_image(coord.state), APP_TITLE,
            menu=pystray.Menu(self._build_menu),
        )

    # ---- menu --------------------------------------------------------

    def _build_menu(self) -> pystray.Menu:
        c = self.coord
        connected = c.link.is_connected()
        peer_txt = "peer: connected" if connected else "peer: offline"

        items = [
            pystray.MenuItem(c.status_line(), None, enabled=False),
            pystray.MenuItem(peer_txt, None, enabled=False),
            pystray.MenuItem(f"mode: {c.link.role}", None, enabled=False),
            pystray.Menu.SEPARATOR,
        ]
        can_use = connected and c.state == ExitState.NONE \
            and c.peer_state != ExitState.USING_OTHER
        items.append(pystray.MenuItem(
            "Use peer as exit node",
            lambda *_: self._run_action(c.use_peer),
            enabled=can_use,
        ))
        items.append(pystray.MenuItem(
            "Stop using peer as exit node",
            lambda *_: self._run_action(c.stop_using_peer),
            enabled=c.state == ExitState.USING_OTHER,
        ))
        items += [pystray.Menu.SEPARATOR, pystray.MenuItem("Quit", lambda *_: self._on_quit())]
        # Menu(*items): each positional arg is one item — passing the list
        # itself would store a bare list as a menu entry and crash pystray.
        return pystray.Menu(*items)

    # ---- actions / updates ---------------------------------------------

    def _run_action(self, fn: Callable[[], tuple[bool, str]]) -> None:
        """Run a coordinator action off the menu thread, report via notification."""

        def worker() -> None:
            try:
                ok, msg = fn()
            except Exception:  # never let the tray die on an action
                log.exception("tray action failed")
                ok, msg = False, "Unexpected error (see log)."
            self.icon.notify(msg, APP_TITLE)
            self.refresh()
            threading.Timer(4.0, self.icon.remove_notification).start()

        threading.Thread(target=worker, daemon=True, name="tray-action").start()

    def refresh(self) -> None:
        self.icon.icon = _make_image(self.coord.state)
        self.icon.title = f"{APP_TITLE} — {LABELS[self.coord.state]}"
        try:
            self.icon.update_icon()
            self.icon.update_menu()
        except Exception:
            pass

    def notify(self, msg: str) -> None:
        self.icon.notify(msg, APP_TITLE)
        threading.Timer(4.0, self.icon.remove_notification).start()

    def run(self) -> None:
        self.icon.run()

    def stop(self) -> None:
        try:
            self.icon.stop()
        except Exception:
            pass
